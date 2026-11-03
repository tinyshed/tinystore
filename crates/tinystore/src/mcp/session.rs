//! The protocol, apart from how a call runs: what each message asks, which
//! tools the allow list offers, the calls under way by their ids, and the
//! answer a call that has finished gets, or none. The session knows each
//! tool by its card and runs no handler: the server that drives it holds the
//! handlers and runs each call it hands out, on a thread of its own as
//! `mcp::Server` does, or as a future the program's runtime polls as
//! `aio::mcp::Server` does.

use std::thread;

use serde_json::{Value, json};

use super::given::meant;
use super::tool::{Answer, Card, Mark};
use crate::cancel::Tokens;
use crate::text::panic_message;
use crate::{CancellationToken, Error, Result};

/// The protocol's version this server speaks when a client names none.
const PROTOCOL: &str = "2025-06-18";

/// What a server says of itself, and its allow list as the program's
/// `--write` gave it.
#[derive(Debug)]
pub(crate) struct Setup {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) instructions: Option<String>,
    pub(crate) allowed: Option<String>,
}

impl Setup {
    pub(crate) fn new(name: &str, version: &str) -> Setup {
        Setup { name: name.to_owned(), version: version.to_owned(), instructions: None, allowed: None }
    }
}

/// A server whose tools and allow list were checked, answering.
pub(crate) struct Session {
    setup: Setup,
    cards: Vec<Card>,
    /// The tools that write or remove which the allow list names.
    writes: Vec<String>,
    /// The token of each call under way, by its id as JSON text, so that
    /// `1` and `"1"` are two ids, as JSON-RPC reads them.
    running: Tokens<String>,
}

/// What a message asks of the server.
pub(crate) enum Asked {
    /// What to answer now: nothing, to a notification.
    Answer(Option<Value>),
    /// A call of a tool the server offers, to run and then give to
    /// [`Session::finished`].
    Call(Call),
}

/// A call under way.
pub(crate) struct Call {
    id: Value,
    /// Where the tool's card is among the session's, and so its handler
    /// among the server's.
    pub(crate) tool: usize,
    pub(crate) arguments: Value,
    /// Cancelled once the host gives up on the call.
    pub(crate) token: CancellationToken,
}

impl Session {
    pub(crate) fn of(setup: Setup, cards: Vec<Card>) -> Result<Session> {
        check_tools(&setup.name, &cards)?;
        let writes = allowed(&cards, setup.allowed.as_deref())?;
        Ok(Session { setup, cards, writes, running: Tokens::default() })
    }

    /// What one line, a message, asks.
    pub(crate) fn received(&self, line: &str) -> Asked {
        if line.trim().is_empty() {
            return Asked::Answer(None);
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => return Asked::Answer(Some(failure(Value::Null, -32700, &format!("not JSON: {error}")))),
        };
        let method = message.get("method").and_then(Value::as_str);
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = message.get("id").cloned() else {
            if method == Some("notifications/cancelled") {
                self.cancel(&params);
            }
            return Asked::Answer(None);
        };
        let result = match method {
            Some("initialize") => self.initialized(&params),
            Some("ping") => json!({}),
            Some("tools/list") => self.listed(),
            Some("tools/call") => return self.call(id, &params),
            Some(other) => return Asked::Answer(Some(failure(id, -32601, &format!("no method {other}")))),
            None => return Asked::Answer(Some(failure(id, -32600, "a request names its method"))),
        };
        Asked::Answer(Some(answer(id, result)))
    }

    /// The answer to a call whose handler has returned, or panicked, which
    /// is the tool's error; none once the call was cancelled: its host has
    /// given up on it, and MCP's spec asks that it get no answer.
    pub(crate) fn finished(&self, call: Call, ran: thread::Result<Answer>) -> Option<Value> {
        // a cancel reached the token before the call left, or it finds nothing and is ignored
        self.dropped(&call);
        if call.token.is_cancelled() {
            return None;
        }
        let answered = ran.unwrap_or_else(|panicked| {
            Err(format!("{} panicked: {}", self.cards[call.tool].name, panic_message(&*panicked)).into())
        });
        Some(answer(call.id, called(answered)))
    }

    /// A call that leaves unanswered, its future dropped once it was
    /// cancelled.
    pub(crate) fn dropped(&self, call: &Call) {
        self.running.finish(&call.id.to_string(), &call.token);
    }

    /// Cancels every call under way, once nobody is left to read an answer.
    pub(crate) fn cancel_all(&self) {
        self.running.cancel_all();
    }

    /// Cancels the call `notifications/cancelled` names; one that is not
    /// under way, or has finished, is ignored.
    fn cancel(&self, params: &Value) {
        if let Some(id) = params.get("requestId") {
            self.running.cancel(&id.to_string());
        }
    }

    fn offers(&self, card: &Card) -> bool {
        card.mark == Mark::Reads || self.writes.contains(&card.name)
    }

    fn initialized(&self, params: &Value) -> Value {
        let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": self.setup.name, "version": self.setup.version },
            "instructions": self.instructions(),
        })
    }

    /// The program's instructions, and which tools change anything: a model
    /// read "Write with dashboard_save alone" as "the program watches the
    /// file, don't write it yourself", so the sentence says it plainly.
    fn instructions(&self) -> String {
        let writing = match &self.writes[..] {
            [] => "Nothing here writes.".to_owned(),
            writes => {
                let changes = if writes.len() == 1 { "changes" } else { "change" };
                let only = format!("Only {} {changes} anything", in_english(writes));
                let reads = self.cards.iter().any(|card| card.mark == Mark::Reads);
                if reads { format!("{only}; every other tool only reads.") } else { format!("{only}.") }
            }
        };
        match &self.setup.instructions {
            Some(given) => format!("{given} {writing}"),
            None => writing,
        }
    }

    fn listed(&self) -> Value {
        let tools: Vec<Value> = self.cards.iter().filter(|card| self.offers(card)).map(Card::listed).collect();
        json!({ "tools": tools })
    }

    /// A call to run, or why the tool is refused, answered at once as the
    /// tool's own error.
    fn call(&self, id: Value, params: &Value) -> Asked {
        let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
        let refused = match self.cards.iter().position(|card| card.name == name) {
            None => format!("no tool {name}"),
            Some(tool) if !self.offers(&self.cards[tool]) => {
                let (does, program) = (self.cards[tool].mark.verb(), &self.setup.name);
                format!("{name} {does}, and {program} mcp was not started with --write {name}")
            }
            Some(tool) => return self.started(id, tool, params),
        };
        Asked::Answer(Some(answer(id, called(Err(refused.into())))))
    }

    /// The call of `tool`, under way from now; refused while a call of the
    /// same id still is, since their answers could not be told apart.
    fn started(&self, id: Value, tool: usize, params: &Value) -> Asked {
        // only the thread that reads the messages starts a call, so none takes the id between
        if self.running.holds(&id.to_string()) {
            let still = format!("a call of id {id} is still running");
            return Asked::Answer(Some(failure(id, -32600, &still)));
        }
        let token = self.running.start(id.to_string());
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        Asked::Call(Call { id, tool, arguments, token })
    }
}

fn answer(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A tool's answer as text, or what went wrong as the tool's own error.
fn called(answered: Answer) -> Value {
    match answered {
        Ok(answer) => json!({ "content": [{ "type": "text", "text": answer.to_string() }] }),
        Err(why) => json!({ "content": [{ "type": "text", "text": why.to_string() }], "isError": true }),
    }
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Names as English joins them: `a`, `a and b`, `a, b and c`.
fn in_english(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// Refuses two tools of one name, which a call could not tell apart, and a
/// schema that is not an object's, which MCP's hosts refuse.
fn check_tools(server: &str, cards: &[Card]) -> Result<()> {
    for (at, card) in cards.iter().enumerate() {
        if cards[..at].iter().any(|before| before.name == card.name) {
            return Err(Error::invalid(format!("mcp server {server}: two tools named {}", card.name)));
        }
        if card.schema().get("type").and_then(Value::as_str) != Some("object") {
            return Err(Error::invalid(format!(
                "mcp tool {}: its input schema is a JSON Schema of an object, as {{\"type\": \"object\"}}",
                card.name
            )));
        }
    }
    Ok(())
}

/// The tools that write or remove which `given` names: `all`, or their names
/// with commas between. A name of none is refused with the nearest.
fn allowed(cards: &[Card], given: Option<&str>) -> Result<Vec<String>> {
    let writes: Vec<&str> =
        cards.iter().filter(|card| card.mark != Mark::Reads).map(|card| card.name.as_str()).collect();
    let named: Vec<&str> = match given {
        None => return Ok(Vec::new()),
        Some("all") => writes.clone(),
        Some(list) => list.split(',').map(str::trim).collect(),
    };
    if let Some(unknown) = named.iter().find(|word| !writes.contains(word)) {
        let offered =
            if writes.is_empty() { "no tool here writes".to_owned() } else { format!("all, or {}", writes.join(", ")) };
        return Err(Error::invalid(format!(
            "--write {unknown}: no tool that writes{}; {offered}",
            meant(unknown, &writes)
        )));
    }
    Ok(writes.iter().filter(|name| named.contains(name)).map(|name| (*name).to_owned()).collect())
}
