//! tinystore's own tools that read a store: its logs, kv values, jobs, SQL
//! databases and search indexes, a tool for each engine built beside them.
//!
//! Each call opens the store again beside whoever holds it, reads, and lets
//! go. So a call meets the streams, buckets and databases made since the last
//! one, and writes nothing: opening a bucket or a queue by a name the store
//! does not hold would make it in a store this process holds, and beside it
//! that is refused instead.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use super::tool::{Tool, Tools};
use crate::Store;

/// What a list answers at most, so that an answer fits an agent's context.
#[cfg(any(feature = "records", feature = "kv", feature = "jobs", feature = "sql"))]
pub(super) const MOST: usize = 200;

/// Opens the store a call reads.
pub(super) type Opener = Arc<dyn Fn() -> crate::Result<Store> + Send + Sync>;

/// tinystore's tools that read `store`, for the engines this program was
/// built with: `logs`, `kv_get`, `kv_list`, `jobs_list`, `sql_query` and
/// `search_list`. They read beside it, so a call sees what was committed
/// before it and writes nothing.
pub fn read_tools(store: &Store) -> Tools {
    read_tools_in(store.dir())
}

/// [`read_tools`] for the store in `dir`, which no process need hold when
/// the tools are made: the command line's, which starts before its program.
pub fn read_tools_in(dir: impl Into<PathBuf>) -> Tools {
    let dir: Arc<Path> = dir.into().into();
    let open: Opener = Arc::new(move || Store::open_beside(&dir));
    let engines: Vec<fn(&Opener) -> Vec<Tool>> = vec![
        #[cfg(feature = "records")]
        super::logs::tools,
        #[cfg(feature = "kv")]
        super::kv::tools,
        #[cfg(feature = "jobs")]
        super::jobs::tools,
        #[cfg(feature = "sql")]
        super::sql::tools,
        #[cfg(feature = "search")]
        super::search::tools,
    ];
    Tools(engines.iter().flat_map(|tools| tools(&open)).collect())
}

/// The store a call reads, or why it could not be opened.
#[cfg(any(feature = "records", feature = "kv", feature = "jobs", feature = "sql", feature = "search"))]
pub(super) fn opened(open: &Opener) -> Result<Store, String> {
    open().map_err(|error| error.to_string())
}

/// A call's arguments, each read as what it should be.
#[cfg(any(feature = "records", feature = "kv", feature = "jobs", feature = "sql"))]
pub(super) struct Arguments<'a> {
    #[cfg_attr(
        not(any(feature = "kv", feature = "jobs", feature = "sql")),
        expect(dead_code, reason = "a tool names itself only when it refuses a call missing what it needs")
    )]
    pub(super) tool: &'static str,
    pub(super) given: &'a Value,
}

#[cfg(any(feature = "records", feature = "kv", feature = "jobs", feature = "sql"))]
impl<'a> Arguments<'a> {
    pub(super) fn text(&self, key: &str) -> Option<&'a str> {
        self.given.get(key).and_then(Value::as_str)
    }

    #[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
    pub(super) fn needed(&self, key: &str) -> Result<&'a str, String> {
        self.text(key).ok_or_else(|| format!("{} takes {key}", self.tool))
    }

    /// The `limit` given, `otherwise` when none was, and `MOST` at most.
    #[cfg(any(feature = "records", feature = "kv", feature = "jobs"))]
    pub(super) fn limit(&self, otherwise: usize) -> usize {
        let given = self.given.get("limit").and_then(Value::as_u64);
        given.map_or(otherwise, |limit| usize::try_from(limit).unwrap_or(usize::MAX)).min(MOST)
    }
}

/// The JSON Schema of an object of `properties`, `required` among them.
#[cfg(any(feature = "records", feature = "kv", feature = "jobs", feature = "sql", feature = "search"))]
pub(super) fn schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

#[cfg(any(feature = "records", feature = "kv", feature = "jobs", feature = "sql"))]
pub(super) fn text(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

#[cfg(any(feature = "records", feature = "kv", feature = "jobs"))]
pub(super) fn count(description: &str) -> Value {
    json!({ "type": "integer", "description": description })
}

/// `properties` and a bucket's or a queue's database: a program that opened
/// one from a database keeps it in that database's file, not in kv.db or
/// jobs.db.
#[cfg(any(feature = "kv", feature = "jobs"))]
pub(super) fn with_database(properties: Value) -> Value {
    #[cfg(feature = "sql")]
    let properties = {
        let mut properties = properties;
        let kept = "the database the program opened it from, as app, when it is kept there rather than in kv.db or \
                    jobs.db";
        properties["database"] = text(kept);
        properties
    };
    properties
}

#[cfg(all(test, feature = "kv"))]
#[path = "store_tests.rs"]
mod tests;
