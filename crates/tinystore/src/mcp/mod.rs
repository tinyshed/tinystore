//! A program's own MCP server: the Model Context Protocol on stdin and
//! stdout, a JSON-RPC message a line, as `tinystore mcp` answers it. A tool is
//! a value with a mark, read, write or remove, which an agent's host reads to
//! ask its person before each tool that changes something; a tool that
//! writes or removes is offered only when the allow list, the program's
//! `--write`, names it. tinystore's own read tools mount beside the
//! program's, on the store it holds.
//!
//! ```no_run
//! use serde_json::json;
//! use tinystore::mcp::{self, Tool};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let write = std::env::args().skip_while(|arg| arg != "--write").nth(1);
//! let name = json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] });
//! mcp::server("dashbin", env!("CARGO_PKG_VERSION"))
//!     .instructions("Read dashbin's dashboards and the store under them.")
//!     .tools(mcp::read_tools(&store).prefix("store_")) // store_logs, store_kv_get, …
//!     .tool(Tool::reads("dashboards_list", "The project's dashboards.", json!({ "type": "object" }), |_, _| {
//!         Ok(json!([{ "name": "latency" }]))
//!     }))
//!     .tool(Tool::writes("dashboard_save", "Saves a dashboard.", name.clone(), |given, _| {
//!         let name = given["name"].as_str().ok_or("dashboard_save takes name")?;
//!         Ok(json!({ "saved": name }))
//!     }))
//!     .tool(Tool::removes("dashboard_delete", "Deletes a dashboard.", name, |_, _| Ok(json!({ "deleted": true }))))
//!     .allow(write.as_deref()) // none, "all", or "dashboard_save,dashboard_delete"
//!     .serve()?; // answers stdin until it closes
//! # Ok(())
//! # }
//! ```
//!
//! Each call runs on a thread of its own. A program on an async runtime
//! serves the same protocol with tools that await, through
//! `tinystore::aio::mcp` and the `async` feature.
//!
//! The modules below the server's own are the reads tinystore's tools make
//! and the JSON they answer, which the command line's `--json` prints too:
//! hidden, since they promise a program nothing.

#[cfg(test)]
pub(crate) mod fixture;
pub(crate) mod server;
pub(crate) mod session;
#[cfg(feature = "store")]
mod store;
pub(crate) mod tool;

#[doc(hidden)]
pub mod given;
#[cfg(feature = "jobs")]
#[doc(hidden)]
pub mod jobs;
#[cfg(feature = "kv")]
#[doc(hidden)]
pub mod kv;
#[cfg(feature = "records")]
#[doc(hidden)]
pub mod logs;
#[cfg(feature = "search")]
mod search;
#[cfg(feature = "sql")]
#[doc(hidden)]
pub mod sql;

pub use server::Server;
#[cfg(feature = "store")]
pub use store::read_tools;
#[cfg(feature = "store")]
#[doc(hidden)]
pub use store::read_tools_in;
pub use tool::{Tool, Tools};

/// A server named `name` at `version`, as the agent's host lists it, with no
/// tool yet.
pub fn server(name: &str, version: &str) -> Server {
    Server::new(name, version)
}
