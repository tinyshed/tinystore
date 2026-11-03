//! A tool an agent calls: its card, which says its name, what it does, the
//! JSON Schema of what it takes and its mark, and the code that answers it.

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::CancellationToken;

/// What a tool answers: its JSON, or why it failed, which the agent reads as
/// the tool's error.
pub(crate) type Answer = Result<Value, Box<dyn StdError + Send + Sync>>;

/// A handler that answers on the thread that calls it.
pub(crate) type Blocking = Arc<dyn Fn(&Value, &CancellationToken) -> Answer + Send + Sync>;

/// What a tool does to what it reaches. The host reads it as MCP's
/// annotations, and asks its person before a tool that is not `Reads`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Reads,
    Writes,
    /// Deletes, or replaces what was there: what a person may want back.
    Removes,
}

impl Mark {
    /// What a tool of this mark does, as a refusal says it.
    pub(crate) fn verb(self) -> &'static str {
        match self {
            Mark::Reads => "reads",
            Mark::Writes => "writes",
            Mark::Removes => "removes",
        }
    }
}

/// What the server knows of a tool apart from its handler, whichever form
/// the handler takes: what it lists and what its allow list reads.
#[derive(Clone, Debug)]
pub(crate) struct Card {
    pub(crate) name: String,
    description: String,
    schema: Value,
    pub(crate) mark: Mark,
}

impl Card {
    pub(crate) fn new(mark: Mark, name: &str, description: &str, schema: Value) -> Card {
        Card { name: name.to_owned(), description: description.to_owned(), schema, mark }
    }

    pub(crate) fn schema(&self) -> &Value {
        &self.schema
    }

    /// The tool as `tools/list` gives it, with MCP's annotations of its mark.
    pub(crate) fn listed(&self) -> Value {
        let annotations = match self.mark {
            Mark::Reads => json!({ "readOnlyHint": true, "openWorldHint": false }),
            Mark::Writes => json!({ "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }),
            Mark::Removes => json!({ "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false }),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.schema,
            "annotations": annotations,
        })
    }
}

/// A tool an agent may call. Its handler takes the call's arguments and its
/// token, and answers JSON, which the agent reads as text, or an error, which
/// it reads as the tool's error with its message.
///
/// The token is cancelled once the agent's host gives up on the call, and the
/// server then writes no answer for it. Every read the handler makes through
/// a store stops by itself, failing `Cancelled`, since the handler runs under
/// the token; work of its own, as a statement on another database, asks
/// [`is_cancelled`](CancellationToken::is_cancelled) and stops:
///
/// ```
/// # use serde_json::{Value, json};
/// # use tinystore::mcp::Tool;
/// # fn ran(panel: &Value) -> Value { panel.clone() }
/// let takes = json!({ "type": "object", "properties": { "panels": { "type": "array" } } });
/// Tool::reads("dashboard_check", "Runs each panel's query.", takes, |given, token| {
///     let mut checked = Vec::new();
///     for panel in given["panels"].as_array().ok_or("dashboard_check takes panels")? {
///         if token.is_cancelled() {
///             return Err("cancelled".into()); // nobody reads it: the host gave up on the call
///         }
///         checked.push(ran(panel));
///     }
///     Ok(json!(checked))
/// });
/// ```
#[derive(Clone)]
pub struct Tool {
    pub(crate) card: Card,
    pub(crate) handler: Blocking,
}

impl Tool {
    /// A tool that only reads, offered always.
    pub fn reads(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(&Value, &CancellationToken) -> Answer + Send + Sync + 'static,
    ) -> Tool {
        Tool { card: Card::new(Mark::Reads, name, description, schema), handler: Arc::new(handler) }
    }

    /// A tool that writes and removes nothing, offered only when the allow
    /// list names it.
    pub fn writes(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(&Value, &CancellationToken) -> Answer + Send + Sync + 'static,
    ) -> Tool {
        Tool { card: Card::new(Mark::Writes, name, description, schema), handler: Arc::new(handler) }
    }

    /// A tool that deletes something or replaces what was there, offered only
    /// when the allow list names it.
    pub fn removes(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(&Value, &CancellationToken) -> Answer + Send + Sync + 'static,
    ) -> Tool {
        Tool { card: Card::new(Mark::Removes, name, description, schema), handler: Arc::new(handler) }
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tool").field("name", &self.card.name).field("mark", &self.card.mark).finish_non_exhaustive()
    }
}

/// Tools mounted together, as [`read_tools`](super::read_tools) gives
/// tinystore's.
#[derive(Debug)]
#[must_use = "tools are mounted with Server::tools"]
pub struct Tools(pub(crate) Vec<Tool>);

impl Tools {
    /// The same tools, each name after `prefix`: `store_` makes `logs`
    /// `store_logs`, so that they sit beside a program's own without a clash.
    pub fn prefix(self, prefix: &str) -> Tools {
        let prefixed = |mut tool: Tool| {
            tool.card.name = format!("{prefix}{}", tool.card.name);
            tool
        };
        Tools(self.0.into_iter().map(prefixed).collect())
    }
}

impl IntoIterator for Tools {
    type Item = Tool;
    type IntoIter = std::vec::IntoIter<Tool>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
