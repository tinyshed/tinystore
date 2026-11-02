//! The server: JSON-RPC 2.0 a line on stdin, an answer a line on stdout,
//! flushed at once, since an agent's host waits for each. It answers
//! `initialize`, `ping`, `tools/list` and `tools/call`, and a notification
//! with nothing. A tool that writes or removes is offered, and runs, only
//! when the allow list names it.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

use super::given::meant;
use super::tool::{Mark, Tool};
use crate::{Error, Result};

/// The protocol's version this server speaks when a client names none.
const PROTOCOL: &str = "2025-06-18";

/// An MCP server being built: its name, its tools and its allow list; it
/// answers with [`serve`](Server::serve).
#[derive(Debug)]
#[must_use = "a server answers with serve()"]
pub struct Server {
    name: String,
    version: String,
    instructions: Option<String>,
    tools: Vec<Tool>,
    allowed: Option<String>,
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
    pub fn serve(self) -> Result<()> {
        self.serve_on(io::stdin().lock(), io::stdout().lock())
    }

    /// [`serve`](Server::serve) on any input and output, as a test drives it.
    #[doc(hidden)]
    pub fn serve_on(self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        let session = Session::of(self)?;
        for line in input.lines() {
            let line = line.map_err(|error| Error::io("mcp: stdin", error))?;
            if line.trim().is_empty() {
                continue;
            }
            let Some(answer) = session.answer(&line) else { continue };
            match writeln!(output, "{answer}").and_then(|()| output.flush()) {
                Ok(()) => {}
                // the host has gone: nobody is left to answer
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
                Err(error) => return Err(Error::io("mcp: stdout", error)),
            }
        }
        Ok(())
    }
}

/// A server whose tools and allow list were checked, answering.
struct Session {
    server: Server,
    /// The tools that write or remove which the allow list names.
    writes: Vec<String>,
}

impl Session {
    fn of(server: Server) -> Result<Session> {
        check_tools(&server)?;
        let writes = allowed(&server.tools, server.allowed.as_deref())?;
        Ok(Session { server, writes })
    }

    fn offers(&self, tool: &Tool) -> bool {
        tool.mark == Mark::Reads || self.writes.contains(&tool.name)
    }

    /// The answer to one message; none to a notification.
    fn answer(&self, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => return Some(failure(Value::Null, -32700, &format!("not JSON: {error}"))),
        };
        let id = message.get("id").cloned()?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match message.get("method").and_then(Value::as_str) {
            Some("initialize") => self.initialized(&params),
            Some("ping") => json!({}),
            Some("tools/list") => {
                let tools: Vec<Value> =
                    self.server.tools.iter().filter(|tool| self.offers(tool)).map(Tool::listed).collect();
                json!({ "tools": tools })
            }
            Some("tools/call") => self.call(&params),
            Some(other) => return Some(failure(id, -32601, &format!("no method {other}"))),
            None => return Some(failure(id, -32600, "a request names its method")),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
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

    /// The program's instructions, and what of the server writes.
    fn instructions(&self) -> String {
        let writing = match &self.writes[..] {
            [] => "Nothing here writes.".to_owned(),
            writes => format!("Write with {} alone, each a change the program sees.", writes.join(", ")),
        };
        match &self.server.instructions {
            Some(given) => format!("{given} {writing}"),
            None => writing,
        }
    }

    /// A tool's answer as text, or what went wrong as the tool's own error.
    fn call(&self, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let answered = match self.server.tools.iter().find(|tool| tool.name == name) {
            None => Err(format!("no tool {name}")),
            Some(tool) if !self.offers(tool) => {
                Err(format!("{name} writes, and {} mcp was not started with --write {name}", self.server.name))
            }
            Some(tool) => tool.call(&arguments).map_err(|error| error.to_string()),
        };
        match answered {
            Ok(answer) => json!({ "content": [{ "type": "text", "text": answer.to_string() }] }),
            Err(why) => json!({ "content": [{ "type": "text", "text": why }], "isError": true }),
        }
    }
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
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

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
