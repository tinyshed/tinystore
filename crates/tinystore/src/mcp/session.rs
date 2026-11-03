//! The protocol, apart from how a call runs: what each message asks, which
//! tools the allow list offers, the calls under way by their ids, and whether
//! a call that has finished is answered. The session runs no handler itself:
//! `Server::serve_on` runs each call it hands out on a thread of its own.

use serde_json::{Value, json};

use super::given::meant;
use super::server::Server;
use super::tool::{Answer, Mark, Tool};
use crate::cancel::Tokens;
use crate::{CancellationToken, Error, Result};

/// The protocol's version this server speaks when a client names none.
const PROTOCOL: &str = "2025-06-18";

/// A server whose tools and allow list were checked, answering.
pub(super) struct Session {
    server: Server,
    /// The tools that write or remove which the allow list names.
    writes: Vec<String>,
    /// The token of each call under way, by its id as JSON text, so that
    /// `1` and `"1"` are two ids, as JSON-RPC reads them.
    running: Tokens<String>,
}

/// What a message asks of the server.
pub(super) enum Asked<'s> {
    /// What to answer now: nothing, to a notification.
    Answer(Option<Value>),
    /// A call of a tool the server offers, to run and then give to
    /// [`Session::finished`].
    Call(Call<'s>),
}

/// A call under way.
pub(super) struct Call<'s> {
    id: Value,
    pub(super) tool: &'s Tool,
    pub(super) arguments: Value,
    /// Cancelled once the host gives up on the call.
    pub(super) token: CancellationToken,
}

impl Session {
    pub(super) fn of(server: Server) -> Result<Session> {
        check_tools(&server)?;
        let writes = allowed(&server.tools, server.allowed.as_deref())?;
        Ok(Session { server, writes, running: Tokens::default() })
    }

    /// What one line, a message, asks.
    pub(super) fn received(&self, line: &str) -> Asked<'_> {
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

    /// The answer to a call whose handler has returned, or none once the call
    /// was cancelled: its host has given up on it, and MCP's spec asks that
    /// it get no answer.
    pub(super) fn finished(&self, call: Call<'_>, answered: Answer) -> Option<Value> {
        // a cancel reached the token before the call left, or it finds nothing and is ignored
        self.running.finish(&call.id.to_string(), &call.token);
        if call.token.is_cancelled() {
            return None;
        }
        Some(answer(call.id, called(answered)))
    }

    /// Cancels every call under way, once nobody is left to read an answer.
    pub(super) fn cancel_all(&self) {
        self.running.cancel_all();
    }

    /// Cancels the call `notifications/cancelled` names; one that is not
    /// under way, or has finished, is ignored.
    fn cancel(&self, params: &Value) {
        if let Some(id) = params.get("requestId") {
            self.running.cancel(&id.to_string());
        }
    }

    fn offers(&self, tool: &Tool) -> bool {
        tool.mark == Mark::Reads || self.writes.contains(&tool.name)
    }

    fn initialized(&self, params: &Value) -> Value {
        let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": self.server.name, "version": self.server.version },
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
                let reads = self.server.tools.iter().any(|tool| tool.mark == Mark::Reads);
                if reads { format!("{only}; every other tool only reads.") } else { format!("{only}.") }
            }
        };
        match &self.server.instructions {
            Some(given) => format!("{given} {writing}"),
            None => writing,
        }
    }

    fn listed(&self) -> Value {
        let tools: Vec<Value> = self.server.tools.iter().filter(|tool| self.offers(tool)).map(Tool::listed).collect();
        json!({ "tools": tools })
    }

    /// A call to run, or why the tool is refused, answered at once as the
    /// tool's own error.
    fn call(&self, id: Value, params: &Value) -> Asked<'_> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
        let refused = match self.server.tools.iter().find(|tool| tool.name == name) {
            None => format!("no tool {name}"),
            Some(tool) if !self.offers(tool) => {
                let (does, program) = (tool.mark.verb(), &self.server.name);
                format!("{name} {does}, and {program} mcp was not started with --write {name}")
            }
            Some(tool) => return self.started(id, tool, params),
        };
        Asked::Answer(Some(answer(id, called(Err(refused.into())))))
    }

    /// The call of `tool`, under way from now; refused while a call of the
    /// same id still is, since their answers could not be told apart.
    fn started<'s>(&'s self, id: Value, tool: &'s Tool, params: &Value) -> Asked<'s> {
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
fn check_tools(server: &Server) -> Result<()> {
    for (at, tool) in server.tools.iter().enumerate() {
        if server.tools[..at].iter().any(|before| before.name == tool.name) {
            return Err(Error::invalid(format!("mcp server {}: two tools named {}", server.name, tool.name)));
        }
        if tool.schema().get("type").and_then(Value::as_str) != Some("object") {
            return Err(Error::invalid(format!(
                "mcp tool {}: its input schema is a JSON Schema of an object, as {{\"type\": \"object\"}}",
                tool.name
            )));
        }
    }
    Ok(())
}

/// The tools that write or remove which `given` names: `all`, or their names
/// with commas between. A name of none is refused with the nearest.
fn allowed(tools: &[Tool], given: Option<&str>) -> Result<Vec<String>> {
    let writes: Vec<&str> =
        tools.iter().filter(|tool| tool.mark != Mark::Reads).map(|tool| tool.name.as_str()).collect();
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
