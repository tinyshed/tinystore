//! A tool an agent calls: its name, what it does, the JSON Schema of what it
//! takes, its mark and the code that answers it.

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use serde_json::{Value, json};

/// What a tool answers: its JSON, or why it failed, which the agent reads as
/// the tool's error.
pub(crate) type Answer = Result<Value, Box<dyn StdError + Send + Sync>>;

type Handler = Arc<dyn Fn(&Value) -> Answer + Send + Sync>;

/// What a tool does to what it reaches. The host reads it as MCP's
/// annotations, and asks its person before a tool that is not `Reads`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Reads,
    Writes,
    /// Deletes, or replaces what was there: what a person may want back.
    Removes,
}

/// A tool an agent may call. Its handler takes the call's arguments and
/// answers JSON, which the agent reads as text, or an error, which it reads as
/// the tool's error with its message.
#[derive(Clone)]
pub struct Tool {
    pub(crate) name: String,
    description: String,
    schema: Value,
    pub(crate) mark: Mark,
    handler: Handler,
}

impl Tool {
    /// A tool that only reads, offered always.
    pub fn reads(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(&Value) -> Answer + Send + Sync + 'static,
    ) -> Tool {
        Tool::marked(Mark::Reads, name, description, schema, Arc::new(handler))
    }

    /// A tool that writes and removes nothing, offered only when the allow
    /// list names it.
    pub fn writes(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(&Value) -> Answer + Send + Sync + 'static,
    ) -> Tool {
        Tool::marked(Mark::Writes, name, description, schema, Arc::new(handler))
    }

    /// A tool that deletes something or replaces what was there, offered only
    /// when the allow list names it.
    pub fn removes(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(&Value) -> Answer + Send + Sync + 'static,
    ) -> Tool {
        Tool::marked(Mark::Removes, name, description, schema, Arc::new(handler))
    }

    fn marked(mark: Mark, name: &str, description: &str, schema: Value, handler: Handler) -> Tool {
        Tool { name: name.to_owned(), description: description.to_owned(), schema, mark, handler }
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

    pub(crate) fn call(&self, arguments: &Value) -> Answer {
        (self.handler)(arguments)
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tool").field("name", &self.name).field("mark", &self.mark).finish_non_exhaustive()
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
        Tools(self.0.into_iter().map(|tool| Tool { name: format!("{prefix}{}", tool.name), ..tool }).collect())
    }
}

impl IntoIterator for Tools {
    type Item = Tool;
    type IntoIter = std::vec::IntoIter<Tool>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
